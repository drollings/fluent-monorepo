//! route.zig — Deterministic parameter resolution for the subagent FSM.
//!
//! Resolves tool parameters through three strategies:
//!   1. Template expansion (command patterns for bash, path patterns for read)
//!   2. In-process guidance explain (via ExplainFn callback — no subprocess)
//!   3. LLM infill with scratchpad context (via LlmInfillFn callback)
//!
//! M10: ExplainCache integration for deduplication of repeated queries.
//! Uses common/hash.QueryCache for session-scoped explain result caching.

const std = @import("std");
const types = @import("types.zig");
const string_mod = @import("common").string;
const hash_mod = @import("common").hash;

pub const RouteSource = enum {
    template,
    guidance,
    llm,
};

pub const RouteResult = struct {
    params: types.ToolParams,
    source: RouteSource,
};

pub const ExplainResult = struct {
    path: ?[]const u8 = null,
    line: ?u32 = null,
    query: []const u8,
    content: ?[]const u8 = null,
};

pub const ExplainFn = fn (allocator: std.mem.Allocator, query: []const u8, db_path: []const u8, workspace: []const u8) ?ExplainResult;

pub const LlmInfillFn = fn (allocator: std.mem.Allocator, prompt: []const u8, system_prompt: []const u8, grammar: ?[]const u8, max_tokens: u32) ?[]const u8;

/// Session-scoped explain result cache for deduplication of repeated queries.
/// Uses FNV-1a64 for O(1) cache key computation. Evicts at max_entries to bound memory.
pub const ExplainCache = struct {
    inner: hash_mod.QueryCache,
    hits: usize = 0,
    misses: usize = 0,

    pub fn init(allocator: std.mem.Allocator) ExplainCache {
        return .{
            .inner = hash_mod.QueryCache.init(allocator),
        };
    }

    pub fn deinit(self: *ExplainCache) void {
        self.inner.deinit();
    }

    pub fn get(self: *ExplainCache, query: []const u8) ?RouteResult {
        const cached = self.inner.get(query) orelse {
            self.misses += 1;
            return null;
        };
        self.hits += 1;
        // Parse the cached JSON back into RouteResult — for cache hits we return
        // a simple template result with the cached summary as raw output
        return .{
            .params = .{
                .action = .explain,
                .query = cached,
            },
            .source = .guidance,
        };
    }

    pub fn put(self: *ExplainCache, query: []const u8, result: RouteResult) !void {
        const summary = if (result.params.query) |q| q else query;
        try self.inner.put(query, summary);
    }

    pub fn stats(self: *const ExplainCache) struct { hits: usize, misses: usize } {
        return .{ .hits = self.hits, .misses = self.misses };
    }
};

pub fn routeParams(
    allocator: std.mem.Allocator,
    item: types.ChecklistItem,
    action: types.ActionType,
    scratchpad_ctx: ?[]const u8,
    explain_fn: ?*const ExplainFn,
    llm_fn: ?*const LlmInfillFn,
    db_path: []const u8,
    workspace: []const u8,
    config_command_allowlist: []const []const u8,
) !RouteResult {
    return routeParamsCached(allocator, item, action, scratchpad_ctx, explain_fn, llm_fn, db_path, workspace, config_command_allowlist, null);
}

pub fn routeParamsCached(
    allocator: std.mem.Allocator,
    item: types.ChecklistItem,
    action: types.ActionType,
    scratchpad_ctx: ?[]const u8,
    explain_fn: ?*const ExplainFn,
    llm_fn: ?*const LlmInfillFn,
    db_path: []const u8,
    workspace: []const u8,
    config_command_allowlist: []const []const u8,
    cache: ?*ExplainCache,
) !RouteResult {
    // Check explain cache for .explain and .read actions
    if (cache != null and (action == .explain or action == .read)) {
        const query = std.mem.trim(u8, item.text, " \t");
        if (cache.?.get(query)) |cached| {
            return cached;
        }
    }

    const result = routeParamsInner(allocator, item, action, scratchpad_ctx, explain_fn, llm_fn, db_path, workspace, config_command_allowlist);

    // Cache explain results for future deduplication
    if (cache != null and action == .explain and result.source == .guidance) {
        const query = std.mem.trim(u8, item.text, " \t");
        cache.?.put(query, result) catch {};
    }

    return result;
}

fn routeParamsInner(
    allocator: std.mem.Allocator,
    item: types.ChecklistItem,
    action: types.ActionType,
    scratchpad_ctx: ?[]const u8,
    explain_fn: ?*const ExplainFn,
    llm_fn: ?*const LlmInfillFn,
    db_path: []const u8,
    workspace: []const u8,
    config_command_allowlist: []const []const u8,
) !RouteResult {
    switch (action) {
        .bash => return routeBash(allocator, item, scratchpad_ctx, llm_fn, config_command_allowlist),
        .read => return routeRead(allocator, item, scratchpad_ctx, explain_fn, db_path, workspace),
        .explain => return routeExplain(allocator, item, explain_fn, db_path, workspace),
        .edit => return routeEdit(allocator, item, scratchpad_ctx, explain_fn, llm_fn, db_path, workspace),
        .diary => return routeDiary(allocator, item),
        .checklist => return routeChecklist(allocator, item),
        .unknown => return .{ .params = .{ .action = .unknown }, .source = .template },
    }
}

fn routeBash(
    allocator: std.mem.Allocator,
    item: types.ChecklistItem,
    scratchpad_ctx: ?[]const u8,
    llm_fn: ?*const LlmInfillFn,
    allowlist: []const []const u8,
) RouteResult {
    const text = std.mem.trim(u8, item.text, " \t");

    if (tryExtractCommand(allocator, text, allowlist)) |cmd| {
        return .{
            .params = .{ .action = .bash, .command = cmd },
            .source = .template,
        };
    }

    if (textHasPath(text)) |path| {
        var buf: std.ArrayList(u8) = .empty;
        buf.appendSlice(allocator, path) catch return .{ .params = .{ .action = .bash }, .source = .template };
        buf.appendSlice(allocator, " ") catch return .{ .params = .{ .action = .bash }, .source = .template };
        const cmd = buf.toOwnedSlice(allocator) catch return .{ .params = .{ .action = .bash }, .source = .template };
        for (allowlist) |allowed| {
            if (std.mem.startsWith(u8, cmd, allowed)) {
                return .{ .params = .{ .action = .bash, .command = cmd }, .source = .template };
            }
        }
        allocator.free(cmd);
    }

    _ = scratchpad_ctx;
    _ = llm_fn;
    return .{ .params = .{ .action = .bash }, .source = .template };
}

fn routeRead(
    allocator: std.mem.Allocator,
    item: types.ChecklistItem,
    scratchpad_ctx: ?[]const u8,
    explain_fn: ?*const ExplainFn,
    db_path: []const u8,
    workspace: []const u8,
) RouteResult {
    const text = std.mem.trim(u8, item.text, " \t");

    if (extractPath(text)) |path| {
        return .{
            .params = .{ .action = .read, .path = allocator.dupe(u8, path) catch path },
            .source = .template,
        };
    }

    _ = scratchpad_ctx;
    if (explain_fn) |fn_ptr| {
        if (fn_ptr(allocator, text, db_path, workspace)) |result| {
            return .{
                .params = .{
                    .action = .read,
                    .path = result.path,
                    .line_start = result.line,
                    .query = result.query,
                },
                .source = .guidance,
            };
        }
    }

    return .{ .params = .{ .action = .read, .query = allocator.dupe(u8, text) catch text }, .source = .template };
}

fn routeExplain(
    allocator: std.mem.Allocator,
    item: types.ChecklistItem,
    explain_fn: ?*const ExplainFn,
    db_path: []const u8,
    workspace: []const u8,
) RouteResult {
    const text = std.mem.trim(u8, item.text, " \t");

    if (explain_fn) |fn_ptr| {
        if (fn_ptr(allocator, text, db_path, workspace)) |result| {
            return .{
                .params = .{
                    .action = .explain,
                    .query = result.query,
                    .path = result.path,
                },
                .source = .guidance,
            };
        }
    }

    return .{
        .params = .{ .action = .explain, .query = allocator.dupe(u8, text) catch text },
        .source = .template,
    };
}

fn routeEdit(
    allocator: std.mem.Allocator,
    item: types.ChecklistItem,
    scratchpad_ctx: ?[]const u8,
    explain_fn: ?*const ExplainFn,
    llm_fn: ?*const LlmInfillFn,
    db_path: []const u8,
    workspace: []const u8,
) RouteResult {
    const text = std.mem.trim(u8, item.text, " \t");

    if (extractPath(text)) |path| {
        return .{
            .params = .{
                .action = .edit,
                .path = allocator.dupe(u8, path) catch path,
                .content = allocator.dupe(u8, text) catch text,
            },
            .source = .template,
        };
    }

    _ = scratchpad_ctx;
    _ = llm_fn;
    if (explain_fn) |fn_ptr| {
        if (fn_ptr(allocator, text, db_path, workspace)) |result| {
            return .{
                .params = .{
                    .action = .edit,
                    .path = result.path,
                    .content = allocator.dupe(u8, text) catch text,
                },
                .source = .guidance,
            };
        }
    }

    return .{
        .params = .{
            .action = .edit,
            .content = allocator.dupe(u8, text) catch text,
        },
        .source = .template,
    };
}

fn routeDiary(allocator: std.mem.Allocator, item: types.ChecklistItem) RouteResult {
    return .{
        .params = .{
            .action = .diary,
            .content = allocator.dupe(u8, item.text) catch item.text,
        },
        .source = .template,
    };
}

fn routeChecklist(_: std.mem.Allocator, item: types.ChecklistItem) RouteResult {
    return .{
        .params = .{
            .action = .checklist,
            .item_index = item.index,
        },
        .source = .template,
    };
}

fn tryExtractCommand(allocator: std.mem.Allocator, text: []const u8, allowlist: []const []const u8) ?[]const u8 {
    const prefixes = &[_][]const u8{ "run ", "execute ", "make ", "zig build", "cargo ", "npm ", "git " };
    const lower_buf = allocator.alloc(u8, text.len) catch return null;
    defer allocator.free(lower_buf);
    for (text, lower_buf) |c, i| lower_buf[i] = std.ascii.toLower(c);

    for (prefixes) |prefix| {
        if (std.mem.startsWith(u8, lower_buf, prefix)) {
            const rest = std.mem.trim(u8, text[prefix.len..], " \t");
            if (rest.len == 0) continue;
            for (allowlist) |allowed| {
                if (std.mem.startsWith(u8, rest, allowed)) {
                    return allocator.dupe(u8, rest) catch return null;
                }
            }
        }
    }

    for (allowlist) |allowed| {
        if (std.mem.startsWith(u8, text, allowed)) {
            return allocator.dupe(u8, text) catch return null;
        }
    }
    return null;
}

fn extractPath(text: []const u8) ?[]const u8 {
    const extensions = &[_][]const u8{ ".zig", ".md", ".py", ".toml", ".json", ".yaml", ".txt" };
    var it = std.mem.tokenizeAny(u8, text, " \t\n\r,\"'");
    while (it.next()) |token| {
        inline for (extensions) |ext| {
            if (std.mem.endsWith(u8, token, ext)) return token;
        }
        if (std.mem.indexOfScalar(u8, token, '/') != null) {
            if (string_mod.looksLikeIdentifier(token[0..@min(token.len, 1)])) continue;
            return token;
        }
    }
    return null;
}

fn textHasPath(text: []const u8) ?[]const u8 {
    return extractPath(text);
}

const testing = std.testing;

test "routeParams: bash action with command template" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const allowlist = &[_][]const u8{"make"};

    const item: types.ChecklistItem = .{ .index = 0, .text = "run make test", .completed = false, .line_number = 1 };
    const result = try routeParams(allocator, item, .bash, null, null, null, "/tmp/db", "/tmp/ws", allowlist);
    try testing.expect(result.params.action == .bash);
    try testing.expect(result.params.command != null);
    if (result.params.command) |cmd| {
        try testing.expect(std.mem.indexOf(u8, cmd, "make") != null);
    }
}

test "routeParams: explain action without explain_fn" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const allowlist = &[_][]const u8{};

    const item: types.ChecklistItem = .{ .index = 0, .text = "explain filterStages", .completed = false, .line_number = 1 };
    const result = try routeParams(allocator, item, .explain, null, null, null, "/tmp/db", "/tmp/ws", allowlist);
    try testing.expect(result.params.action == .explain);
    try testing.expect(result.params.query != null);
    try testing.expect(result.source == .template);
}

test "routeParams: read action with path detection" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const allowlist = &[_][]const u8{};

    const item: types.ChecklistItem = .{ .index = 0, .text = "read src/main.zig", .completed = false, .line_number = 1 };
    const result = try routeParams(allocator, item, .read, null, null, null, "/tmp/db", "/tmp/ws", allowlist);
    try testing.expect(result.params.action == .read);
    try testing.expect(result.params.path != null);
}

test "routeParams: diary action always completes" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const allowlist = &[_][]const u8{};

    const item: types.ChecklistItem = .{ .index = 0, .text = "document the API", .completed = false, .line_number = 1 };
    const result = try routeParams(allocator, item, .diary, null, null, null, "/tmp/db", "/tmp/ws", allowlist);
    try testing.expect(result.params.action == .diary);
    try testing.expect(result.params.isComplete());
}

test "ExplainCache: put and get" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var cache = ExplainCache.init(allocator);
    defer cache.deinit();

    const result: RouteResult = .{
        .params = .{ .action = .explain, .query = "filterStages" },
        .source = .guidance,
    };
    try cache.put("explain filterStages", result);

    const cached = cache.get("explain filterStages");
    try testing.expect(cached != null);
    if (cached) |r| {
        try testing.expect(r.params.action == .explain);
        try testing.expect(r.source == .guidance);
    }
}

test "ExplainCache: stats tracking" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var cache = ExplainCache.init(allocator);
    defer cache.deinit();

    try testing.expectEqual(@as(usize, 0), cache.stats().hits);
    try testing.expectEqual(@as(usize, 0), cache.stats().misses);

    const result: RouteResult = .{
        .params = .{ .action = .explain, .query = "test" },
        .source = .guidance,
    };
    try cache.put("test query", result);

    const hit = cache.get("test query");
    try testing.expect(hit != null);
    try testing.expectEqual(@as(usize, 1), cache.stats().hits);

    const miss = cache.get("nonexistent");
    try testing.expect(miss == null);
    try testing.expectEqual(@as(usize, 1), cache.stats().misses);
}
