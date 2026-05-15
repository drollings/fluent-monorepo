//! classify.zig — Deterministic classification + batch LLM fallback.
//!
//! Implements TodoAction function-pointer array (QueryMatch pattern) for
//! deterministic classification, and classifyBatchViaLlm() for unknown items.
//! Uses common/string predicates for all deterministic actions and supports
//! an optional IntentClassFn callback for deeper question-form classification.

const std = @import("std");
const types = @import("types.zig");
const string_mod = @import("common").string;

pub const TodoAction = struct {
    matches: *const fn (item_text: []const u8) bool,
    action: types.ActionType,
    priority: u8,
};

fn bashMatches(text: []const u8) bool {
    return string_mod.containsAny(text, &.{
        "run ",   "execute ", "build ", "make ", "zig build",
        "cargo ", "npm ",     "git ",   "test ", "bench ",
    }) or std.mem.startsWith(u8, text, "Run ") or std.mem.startsWith(u8, text, "run ");
}

fn explainMatches(text: []const u8) bool {
    return string_mod.containsAny(text, &.{
        "explain ", "how does ", "what is ", "where is ",
        "find ",    "search ",   "query ",   "look up ",
    });
}

fn readMatches(text: []const u8) bool {
    if (string_mod.containsAny(text, &.{
        "read ",    "examine ",    "review ",  "look at ",
        "inspect ", "understand ", "analyze ", "check file",
    })) return true;
    return string_mod.hasExtension(text, &.{ ".zig", ".md", ".py", ".toml", ".json" });
}

fn editMatches(text: []const u8) bool {
    return string_mod.containsAny(text, &.{
        "fix ",      "implement ", "add ",    "remove ",
        "refactor ", "update ",    "change ", "modify ",
        "replace ",  "write ",     "create ", "delete ",
    });
}

fn docMatches(text: []const u8) bool {
    return string_mod.containsAny(text, &.{
        "document ",        "update docs", "update readme",
        "update structure", "add comment",
    });
}

fn checklistMatches(text: []const u8) bool {
    return string_mod.containsAny(text, &.{
        "check off",   "mark complete", "mark done",
        "toggle item",
    });
}

pub const classifiers = [_]TodoAction{
    .{ .matches = bashMatches, .action = .bash, .priority = 0 },
    .{ .matches = explainMatches, .action = .explain, .priority = 1 },
    .{ .matches = readMatches, .action = .read, .priority = 2 },
    .{ .matches = editMatches, .action = .edit, .priority = 3 },
    .{ .matches = docMatches, .action = .diary, .priority = 4 },
    .{ .matches = checklistMatches, .action = .checklist, .priority = 5 },
};

pub fn classifyDeterministic(item_text: []const u8) types.ActionType {
    const trimmed = std.mem.trim(u8, item_text, " \t");
    if (trimmed.len == 0) return .unknown;
    for (&classifiers) |classifier| {
        if (classifier.matches(trimmed)) return classifier.action;
    }
    return .unknown;
}

pub fn classifyAllDeterministic(items: []const types.ChecklistItem) std.AutoHashMap(usize, types.ActionType) {
    var map = std.AutoHashMap(usize, types.ActionType).init(std.heap.page_allocator);
    for (items) |item| {
        if (!item.completed) {
            const action = classifyDeterministic(item.text);
            if (action != .unknown) {
                map.put(item.index, action) catch {};
            }
        }
    }
    return map;
}

pub const BatchClassifyResult = struct {
    classified: []types.BatchClassifyEntry,
    unknown_indices: []const usize,
};

pub fn batchClassifyDeterministic(
    allocator: std.mem.Allocator,
    items: []const types.ChecklistItem,
) !BatchClassifyResult {
    var classified_list: std.ArrayList(types.BatchClassifyEntry) = .empty;
    errdefer classified_list.deinit(allocator);
    var unknown_list: std.ArrayList(usize) = .empty;
    errdefer unknown_list.deinit(allocator);

    for (items) |item| {
        if (item.completed) continue;
        const action = classifyDeterministic(item.text);
        if (action != .unknown) {
            try classified_list.append(allocator, .{
                .item_index = item.index,
                .action = action,
            });
        } else {
            try unknown_list.append(allocator, item.index);
        }
    }

    return .{
        .classified = try classified_list.toOwnedSlice(allocator),
        .unknown_indices = try unknown_list.toOwnedSlice(allocator),
    };
}

pub fn parseBatchClassifyResult(
    allocator: std.mem.Allocator,
    json_text: []const u8,
    unknown_indices: []const usize,
) ![]types.BatchClassifyEntry {
    var parsed = try std.json.parseFromSlice(std.json.Value, allocator, json_text, .{});
    defer parsed.deinit();

    const root = parsed.value;
    if (root != .array) return error.InvalidJsonFormat;

    var entries: std.ArrayList(types.BatchClassifyEntry) = .empty;
    errdefer entries.deinit(allocator);

    for (root.array.items) |item| {
        if (item != .object) continue;
        const index_val = item.object.get("index") orelse continue;
        const action_val = item.object.get("action") orelse continue;
        if (index_val != .integer or action_val != .string) continue;

        const idx: usize = @intCast(index_val.integer);
        const action_str = action_val.string;
        const action = std.meta.stringToEnum(types.ActionType, action_str) orelse .explain;

        var params: ?types.ToolParams = null;
        if (item.object.get("params")) |params_val| {
            if (params_val == .object) {
                var p: types.ToolParams = .{ .action = action };
                if (params_val.object.get("command")) |v| {
                    if (v == .string) p.command = try allocator.dupe(u8, v.string);
                }
                if (params_val.object.get("path")) |v| {
                    if (v == .string) p.path = try allocator.dupe(u8, v.string);
                }
                if (params_val.object.get("query")) |v| {
                    if (v == .string) p.query = try allocator.dupe(u8, v.string);
                }
                if (params_val.object.get("content")) |v| {
                    if (v == .string) p.content = try allocator.dupe(u8, v.string);
                }
                if (params_val.object.get("item_index")) |v| {
                    if (v == .integer) p.item_index = @intCast(v.integer);
                }
                params = p;
            }
        }

        try entries.append(allocator, .{
            .item_index = if (idx < unknown_indices.len) unknown_indices[idx] else idx,
            .action = action,
            .params = params,
        });
    }

    return entries.toOwnedSlice(allocator);
}

pub fn constitutionallyValidate(
    classified: []types.BatchClassifyEntry,
    items: []const types.ChecklistItem,
) void {
    for (classified) |*entry| {
        if (entry.item_index >= items.len) continue;
        const text = items[entry.item_index].text;
        const det_action = classifyDeterministic(text);
        if (det_action != .unknown and det_action != entry.action) {
            entry.action = det_action;
        }
    }
}

pub const LlmClassifyFn = fn (allocator: std.mem.Allocator, prompt: []const u8, system_prompt: []const u8, grammar: ?[]const u8, max_tokens: u32) ?[]const u8;

pub fn classifyBatchViaLlm(
    allocator: std.mem.Allocator,
    items: []const types.ChecklistItem,
    unknown_indices: []const usize,
    llm_fn: *const LlmClassifyFn,
) ![]types.BatchClassifyEntry {
    if (unknown_indices.len == 0) return &[_]types.BatchClassifyEntry{};

    var prompt_buf: std.ArrayList(u8) = .empty;
    errdefer prompt_buf.deinit(allocator);
    try prompt_buf.appendSlice(allocator, "Classify each task item by its action type. Respond with a JSON array.\nPossible actions: bash, read, explain, edit, diary, checklist\n\n");
    for (unknown_indices) |idx| {
        if (idx < items.len) {
            var aw: std.Io.Writer.Allocating = .init(allocator);
            errdefer aw.deinit();
            try aw.writer.print("[{d}] {s}\n", .{ idx, items[idx].text });
            try prompt_buf.appendSlice(allocator, aw.written());
            aw.deinit();
        }
    }

    const grammar = @import("grammar.zig").classification_grammar;
    const result_text = llm_fn(allocator, prompt_buf.items, "You are a task classifier. Respond with only a JSON array of {index, action} objects.", grammar, 2000) orelse {
        var fallback: std.ArrayList(types.BatchClassifyEntry) = .empty;
        for (unknown_indices) |idx| {
            try fallback.append(allocator, .{ .item_index = idx, .action = .explain });
        }
        return fallback.toOwnedSlice(allocator);
    };
    defer allocator.free(result_text);

    const parsed_entries = parseBatchClassifyResult(allocator, result_text, unknown_indices) catch {
        var fallback: std.ArrayList(types.BatchClassifyEntry) = .empty;
        for (unknown_indices) |idx| {
            try fallback.append(allocator, .{ .item_index = idx, .action = .explain });
        }
        return fallback.toOwnedSlice(allocator);
    };

    constitutionallyValidate(parsed_entries, items);
    return parsed_entries;
}

const testing = std.testing;

test "classifyDeterministic: bash patterns" {
    try testing.expectEqual(types.ActionType.bash, classifyDeterministic("run tests"));
    try testing.expectEqual(types.ActionType.bash, classifyDeterministic("execute make test"));
    try testing.expectEqual(types.ActionType.bash, classifyDeterministic("build the project"));
    try testing.expectEqual(types.ActionType.bash, classifyDeterministic("Run zig build test"));
}

test "classifyDeterministic: explain patterns" {
    try testing.expectEqual(types.ActionType.explain, classifyDeterministic("explain how filterStages works"));
    try testing.expectEqual(types.ActionType.explain, classifyDeterministic("what is GuidanceDb"));
    try testing.expectEqual(types.ActionType.explain, classifyDeterministic("find the query engine"));
}

test "classifyDeterministic: read patterns" {
    try testing.expectEqual(types.ActionType.read, classifyDeterministic("read src/main.zig"));
    try testing.expectEqual(types.ActionType.read, classifyDeterministic("examine the config file"));
    try testing.expectEqual(types.ActionType.read, classifyDeterministic("review src/guidance/staged.zig"));
}

test "classifyDeterministic: edit patterns" {
    try testing.expectEqual(types.ActionType.edit, classifyDeterministic("fix the bug in main.zig"));
    try testing.expectEqual(types.ActionType.edit, classifyDeterministic("implement the new feature"));
    try testing.expectEqual(types.ActionType.edit, classifyDeterministic("add error handling"));
}

test "classifyDeterministic: diary patterns" {
    try testing.expectEqual(types.ActionType.diary, classifyDeterministic("document the API"));
    try testing.expectEqual(types.ActionType.diary, classifyDeterministic("update docs for the module"));
}

test "classifyDeterministic: file extension detection" {
    try testing.expectEqual(types.ActionType.read, classifyDeterministic("src/main.zig"));
    try testing.expectEqual(types.ActionType.read, classifyDeterministic("config.toml"));
}

test "classifyDeterministic: unknown patterns" {
    try testing.expectEqual(types.ActionType.unknown, classifyDeterministic("something completely random"));
    try testing.expectEqual(types.ActionType.unknown, classifyDeterministic("  "));
}

test "classifyDeterministic: priority ordering" {
    try testing.expectEqual(types.ActionType.bash, classifyDeterministic("run tests and check file.zig"));
}

test "batchClassifyDeterministic: separates known and unknown" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const items = [_]types.ChecklistItem{
        .{ .index = 0, .text = "run make test", .completed = false, .line_number = 1 },
        .{ .index = 1, .text = "fix the bug", .completed = false, .line_number = 2 },
        .{ .index = 2, .text = "some mystery task", .completed = false, .line_number = 3 },
        .{ .index = 3, .text = "already done", .completed = true, .line_number = 4 },
    };

    const result = try batchClassifyDeterministic(allocator, &items);
    defer {
        allocator.free(result.classified);
        allocator.free(result.unknown_indices);
    }

    try testing.expect(result.classified.len >= 2);
    try testing.expectEqual(@as(usize, 1), result.unknown_indices.len);
    try testing.expectEqual(@as(usize, 2), result.unknown_indices[0]);
}

test "constitutionallyValidate: downgrades LLM misclassification" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    _ = gpa.allocator();

    const items = [_]types.ChecklistItem{
        .{ .index = 0, .text = "Read foo.zig and understand it", .completed = false, .line_number = 1 },
        .{ .index = 1, .text = "run make test", .completed = false, .line_number = 2 },
    };

    var entries = [_]types.BatchClassifyEntry{
        .{ .item_index = 0, .action = .bash },
        .{ .item_index = 1, .action = .edit },
    };

    constitutionallyValidate(&entries, &items);
    try testing.expectEqual(types.ActionType.read, entries[0].action);
    try testing.expectEqual(types.ActionType.bash, entries[1].action);
}
